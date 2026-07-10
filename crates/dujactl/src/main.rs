//! `dujactl` — scriptable, one-shot control of Duja's displays.
//!
//! `dujactl` talks to the **running Duja app over local IPC** (P5) when it is
//! up, and falls back to the **direct in-process backends** (`duja-ddc` +
//! `duja-panel`) when no app is running. It is a thin dispatcher — [`cli`]
//! parses, [`run`] performs one command's I/O (choosing IPC vs direct, and
//! delegating the IPC path to [`ipc`]), and `main` maps the result to an exit
//! code.
//!
//! Command surface (`--help`): `list`, `get <id>`,
//! `set <id|all> brightness <0-100>`, `doctor`, `version`, with a global
//! `-v`/`--verbose` that reports which path served each request.
//! Exit codes: 0 ok / 2 usage / 3 unknown display / 4 backend error /
//! 5 IPC server error.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::process::ExitCode;

mod backend;
mod cli;
mod fmt;
mod ipc;
mod run;

use cli::Command;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(dispatch(&args))
}

/// Parse and run one invocation, returning the process exit code.
fn dispatch(args: &[String]) -> u8 {
    let (verbose, rest) = extract_verbose(args);
    match cli::parse(&rest) {
        Ok(Command::Help) => {
            println!("{}", cli::USAGE);
            cli::EXIT_OK
        }
        Ok(Command::List) => run::list(verbose),
        Ok(Command::Get { id }) => run::get(&id, verbose),
        Ok(Command::Set { target, percent }) => run::set(&target, percent, verbose),
        // Input switching stays on the direct backend: a paced 0x60 probe/write
        // needs the hardware path, and the IPC surface deliberately omits it.
        Ok(Command::Input { id, value }) => run::input(&id, value.as_deref()),
        Ok(Command::Doctor) => run::doctor(),
        Ok(Command::Version) => run::version(),
        Err(err) => {
            eprintln!("{err}");
            cli::EXIT_USAGE
        }
    }
}

/// Strip the global `-v`/`--verbose` flag from anywhere in the argument list,
/// returning whether it was present and the remaining arguments.
fn extract_verbose(args: &[String]) -> (bool, Vec<String>) {
    let mut verbose = false;
    let rest = args
        .iter()
        .filter(|a| {
            if a.as_str() == "-v" || a.as_str() == "--verbose" {
                verbose = true;
                false
            } else {
                true
            }
        })
        .cloned()
        .collect();
    (verbose, rest)
}

#[cfg(test)]
mod tests {
    use super::extract_verbose;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn verbose_is_extracted_from_anywhere() {
        let (v, rest) = extract_verbose(&args(&["list", "--verbose"]));
        assert!(v);
        assert_eq!(rest, args(&["list"]));

        let (v, rest) = extract_verbose(&args(&["-v", "get", "GSM-1"]));
        assert!(v);
        assert_eq!(rest, args(&["get", "GSM-1"]));

        let (v, rest) = extract_verbose(&args(&["list"]));
        assert!(!v);
        assert_eq!(rest, args(&["list"]));
    }
}

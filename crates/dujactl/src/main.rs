//! `dujactl` — scriptable, one-shot control of Duja's displays.
//!
//! This phase (P3) `dujactl` talks **directly** to the in-process backends
//! (`duja-ddc` + `duja-panel`): no running daemon, no IPC (that transport
//! arrives in P5). It is a thin dispatcher — [`cli`] parses, [`run`] performs
//! one command's I/O, and `main` maps the result to an exit code.
//!
//! Command surface (`--help`): `list`, `get <id>`,
//! `set <id|all> brightness <0-100>`, `doctor`, `version`.
//! Exit codes: 0 ok / 2 usage / 3 unknown display / 4 backend error.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::process::ExitCode;

mod backend;
mod cli;
mod fmt;
mod run;

use cli::Command;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ExitCode::from(dispatch(&args))
}

/// Parse and run one invocation, returning the process exit code.
fn dispatch(args: &[String]) -> u8 {
    match cli::parse(args) {
        Ok(Command::Help) => {
            println!("{}", cli::USAGE);
            cli::EXIT_OK
        }
        Ok(Command::List) => run::list(),
        Ok(Command::Get { id }) => run::get(&id),
        Ok(Command::Set { target, percent }) => run::set(&target, percent),
        Ok(Command::Doctor) => run::doctor(),
        Ok(Command::Version) => run::version(),
        Err(err) => {
            eprintln!("{err}");
            cli::EXIT_USAGE
        }
    }
}

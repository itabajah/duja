//! `cargo xtask` — workspace automation.
//!
//! Tasks land alongside the phases that need them:
//! `dist` (portable Windows packaging), `licenses` (cargo-about bundling, P5),
//! `tr-extract` (Slint translation extraction, P4).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod dist;

use std::process::ExitCode;

const HELP: &str = "\
xtask — Duja workspace automation

USAGE: cargo xtask <task>

TASKS:
  help                       show this help
  dist --version X.Y.Z       stage the portable Windows zip from target/release
  (licenses, tr-extract arrive in later phases)
";

fn main() -> ExitCode {
    let mut args = std::env::args();
    let _bin = args.next(); // argv[0]
    match args.next().as_deref() {
        Some("help") | None => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("dist") => match dist::run(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("xtask dist: {msg}");
                ExitCode::from(1)
            }
        },
        Some(other) => {
            eprintln!("xtask: unknown task `{other}`\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}

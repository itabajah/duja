//! `cargo xtask` — workspace automation.
//!
//! Tasks land alongside the phases that need them:
//! `dist` (P5+ packaging), `licenses` (cargo-about bundling, P5),
//! `tr-extract` (Slint translation extraction, P4).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::process::ExitCode;

const HELP: &str = "\
xtask — Duja workspace automation

USAGE: cargo xtask <task>

TASKS:
  help        show this help
  (dist, licenses, tr-extract arrive in later phases)
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("help") | None => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("xtask: unknown task `{other}`\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}

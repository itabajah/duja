//! `cargo xtask` — workspace automation.
//!
//! Tasks land alongside the phases that need them:
//! `dist` (portable Windows packaging and the macOS `.app`/DMG), `licenses`
//! (cargo-about bundling, P5), `tr-extract` (Slint translation extraction, P4).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod bundle;
mod dist;
mod macho;
mod version;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const HELP: &str = "\
xtask — Duja workspace automation

USAGE: cargo xtask <task>

TASKS:
  help                       show this help
  dist --version X.Y.Z       stage the shippable artifact for this host
                             (Windows: portable zip; macOS: Duja.app + DMG)
       [--target windows|macos]  package for a platform other than the host
       [--sign <identity>]       codesign identity for the macOS bundle
                                 (default `-`, an ad-hoc signature)
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

/// The repository root — the parent of this crate's manifest directory.
///
/// # Errors
/// Returns a message if the manifest directory has no parent, which would mean
/// the crate has been moved out of the workspace.
fn repo_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot resolve the repository root".to_owned())
}

/// Read a repository file by path components, for the tests that pin a constant
/// here against the *other* file that has to agree with it.
///
/// Several couplings in this crate cross a boundary no type can span — a
/// constant in another crate, a value in a YAML workflow, a `[[bin]]` name in a
/// manifest. Restating them in a comment is what rots; reading the other file is
/// what does not. Panics with the path if it cannot be read, which is the
/// intended behaviour: a moved file must fail loudly, not silently stop checking.
#[cfg(test)]
fn read_repo_file(parts: &[&str]) -> String {
    let mut path = repo_root().expect("repo root");
    for part in parts {
        path.push(part);
    }
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

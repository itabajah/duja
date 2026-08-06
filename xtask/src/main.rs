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

#[cfg(test)]
mod tests {
    /// The number of `|`-delimited fields a GitHub-flavoured Markdown row splits
    /// into, counting a backslash-escaped `\|` as content rather than a
    /// separator.
    ///
    /// Returns the raw field count, leading and trailing empties included, so a
    /// well-formed four-column row answers 6. The comparison is row-against-row,
    /// so the two extras cancel; they are kept because subtracting them here
    /// would make an underflow possible on a line with no pipes at all.
    fn unescaped_cells(line: &str) -> usize {
        let mut fields: usize = 1;
        let mut escaped = false;
        for byte in line.bytes() {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'|' => fields = fields.saturating_add(1),
                _ => {}
            }
        }
        fields
    }

    /// Every row of a Markdown table in `docs/` must have exactly as many cells
    /// as its header.
    ///
    /// This exists because a row with an **extra** cell is invisible: GitHub's
    /// Markdown silently drops cells past the header count, so the page renders
    /// correctly while the file contains text nobody reads. `docs/debt.md` is
    /// read by grepping far more often than by rendering, which is the worst
    /// combination of those two facts — and it is exactly how a stale,
    /// already-retracted "Why deferred" cell survived a correction to the row
    /// above it in `#132`.
    ///
    /// Fenced code blocks are skipped: a table drawn inside one is illustration,
    /// not data.
    ///
    /// Cells are counted the way GitHub counts them, which means honouring
    /// `\|` — a backslash-escaped pipe is a literal character, not a separator,
    /// **including inside a code span**. That is not a nicety: an unescaped `|`
    /// in something like `` `O_NOFOLLOW | O_DIRECTORY` `` splits the cell it sits
    /// in, shunts every later cell one column left, and drops the last one off
    /// the end. This check found exactly that in a row written at v0.1.1, whose
    /// "Why deferred" cell had been invisible on the rendered page ever since.
    #[test]
    fn every_docs_table_row_matches_its_header() {
        for file in [
            "debt.md",
            "STATUS.md",
            "qa-checklist.md",
            "release-checklist.md",
        ] {
            let text = crate::read_repo_file(&["docs", file]);
            let mut header: Option<(usize, usize)> = None;
            let mut fenced = false;
            for (number, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.starts_with("```") {
                    fenced = !fenced;
                    continue;
                }
                if fenced {
                    continue;
                }
                if !(line.starts_with('|') && line.ends_with('|')) {
                    // A blank or prose line ends the table; the next `|` line is
                    // a new header.
                    header = None;
                    continue;
                }
                let cells = unescaped_cells(line);
                match header {
                    // The `|---|---|` separator is a row like any other and is
                    // checked against the header the same way.
                    Some((width, at)) => assert_eq!(
                        cells,
                        width,
                        "docs/{file} line {} has {} cells, but the header at line {} has {}",
                        number + 1,
                        cells - 2,
                        at,
                        width - 2
                    ),
                    None => header = Some((cells, number + 1)),
                }
            }
        }
    }
}

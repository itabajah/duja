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
    use std::path::PathBuf;

    /// The number of `|`-delimited fields a GitHub-flavoured Markdown row splits
    /// into, with a leading and a trailing pipe discounted.
    ///
    /// Two rules from the [GFM tables extension], both load-bearing:
    ///
    /// - **`\|` is content, not a separator, *including inside other inline
    ///   spans*.** The spec says exactly that, and gives `` b `\|` az `` as its
    ///   example. So an unescaped `|` inside backticks really does split the
    ///   cell — which is how a `` `O_NOFOLLOW | O_DIRECTORY` `` in `debt.md`
    ///   silently shunted a row's cells one column left and dropped its last one.
    /// - **A leading and trailing pipe is optional.** Discounting one of each
    ///   means both styles count the same, so a row written without a trailing
    ///   pipe cannot raise a false alarm here. (A row written without a *leading*
    ///   one never reaches this function — see the test's own docs.)
    ///
    /// One shape is knowingly not GFM's: a line that is a bare `|` answers 1
    /// here, where cmark-gfm's row parser consumes the leading pipe and ends with
    /// no columns at all. Unreachable — no line under `docs/` is a bare pipe, and
    /// such a line is not a table row in any case — and named so that "counts
    /// cells the way GitHub counts them" is read as the near-identity it is
    /// rather than as an equivalence.
    ///
    /// [GFM tables extension]: https://github.github.com/gfm/#tables-extension-
    fn cell_count(line: &str) -> usize {
        let mut fields: usize = 1;
        let mut escaped = false;
        let mut ends_on_separator = false;
        for byte in line.bytes() {
            // Both decisions read the state as it was on *entry*: a `|` counts
            // when the byte before it did not escape it, and a `\` escapes the
            // next byte only when it was not itself escaped. Testing the updated
            // flag instead — which the first version of this loop did — counts an
            // escaped pipe as a delimiter, because the backslash has already
            // cleared the flag by the time the pipe is looked at.
            let was_escaped = escaped;
            escaped = !was_escaped && byte == b'\\';
            ends_on_separator = byte == b'|' && !was_escaped;
            if ends_on_separator {
                fields = fields.saturating_add(1);
            }
        }
        // A leading pipe contributes an empty first field and a trailing one an
        // empty last. Both discounts come from the same state machine that
        // counted the fields — an earlier version tested the tail with
        // `!line.ends_with("\\|")`, which disagrees with the loop on a row ending
        // in an escaped backslash followed by a real delimiter, and a checker
        // whose two halves can disagree is worth less than the bug it looks for.
        // `saturating_sub` rather than `-` so a pipe-less line (which the caller
        // never passes, but a future one might) cannot underflow, and the length
        // guard stops a lone `"|"` being discounted twice.
        let lead = usize::from(line.starts_with('|'));
        let trail = usize::from(ends_on_separator && line.len() > 1);
        fields.saturating_sub(lead).saturating_sub(trail)
    }

    /// Every Markdown file under `docs/`, recursively.
    fn markdown_files(dir: &PathBuf) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let entries =
            std::fs::read_dir(dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                found.extend(markdown_files(&path));
            } else if path.extension().is_some_and(|ext| ext == "md") {
                found.push(path);
            }
        }
        found
    }

    /// Every row of every Markdown table under `docs/` **that this check can
    /// see** must have exactly as many cells as its own header.
    ///
    /// The qualification is not throat-clearing: a table whose rows omit the
    /// leading pipe is skipped rather than checked, so "every" would be an
    /// over-claim in a coverage sense rather than a false-alarm one. The
    /// paragraph below lists that with the four constructs that would produce a
    /// *wrong* answer rather than no answer.
    ///
    /// GFM does not require this — "if there are a number of cells fewer than the
    /// number of cells in the header row, empty cells are inserted; if there are
    /// greater, the excess is ignored" — and that permissiveness is the whole
    /// problem. An **extra** cell renders as a correct table and leaves text in
    /// the file that nobody reads, which is the worst possible combination for
    /// `debt.md`, a document consulted by grepping far more often than by
    /// rendering. It is how a stale, already-retracted "Why deferred" cell
    /// survived a correction to the row above it in `#132`, and how another row's
    /// last cell had been invisible since v0.1.1.
    ///
    /// The delimiter row is checked like any other, which is stricter than
    /// necessary in one direction and exactly right in the other: GFM says a
    /// header and delimiter that disagree mean **no table is recognised at all**,
    /// so that mismatch turns a table into a paragraph of pipes.
    ///
    /// Fenced code blocks are skipped: a table drawn inside one is illustration,
    /// not data.
    ///
    /// Four things it deliberately does not model, none of which any file under
    /// `docs/` uses today: `~~~` fences (only backticks toggle), indented code
    /// blocks, raw HTML blocks (`docs/qa-checklist.md` has multi-line `<!-- -->`
    /// comments, though none with a pipe in it), and the requirement that a
    /// delimiter row actually follow the presumed header — so a `|`-wrapped prose
    /// line would be taken for a header and could fail the real table beneath it.
    /// Each is a line of code and a class of false alarm; they are named here so
    /// that a future false alarm is diagnosable rather than mysterious.
    ///
    /// A row with **no leading pipe** is a fifth, and a different shape: those are
    /// not scanned at all rather than scanned wrongly, so such a table is silently
    /// unchecked. GFM allows it; nothing under `docs/` writes it.
    #[test]
    fn every_docs_table_row_matches_its_header() {
        let mut docs = crate::repo_root().expect("repo root");
        docs.push("docs");
        let files = markdown_files(&docs);
        let mut rows_checked = 0_usize;

        for path in &files {
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            let name = path.strip_prefix(&docs).unwrap_or(path).display();
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
                if !line.starts_with('|') {
                    // Any non-row line ends the table; the next one that starts
                    // with a pipe is a fresh header.
                    header = None;
                    continue;
                }
                let cells = cell_count(line);
                match header {
                    Some((width, at)) => {
                        rows_checked = rows_checked.saturating_add(1);
                        assert_eq!(
                            cells,
                            width,
                            "docs/{name} line {} has {cells} cells; its header at line {at} has \
                             {width}. An extra cell renders correctly and is read by nobody — \
                             see this test's docs.",
                            number.saturating_add(1),
                        );
                    }
                    None => header = Some((cells, number.saturating_add(1))),
                }
            }
        }

        // The guard that stops this check quietly covering nothing. Its first
        // version named four files, two of which have no tables at all, and
        // missed six that do — including the ADR this project tells new backends
        // to read. A walk plus a floor cannot rot the same way.
        assert!(
            files.len() >= 10,
            "only {} markdown files under docs/ — the walk is not walking",
            files.len()
        );
        assert!(
            rows_checked >= 200,
            "only {rows_checked} table rows checked — the tables are no longer being found"
        );
    }
}
